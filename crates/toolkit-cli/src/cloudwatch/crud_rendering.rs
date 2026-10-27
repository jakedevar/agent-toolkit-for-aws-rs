//! Formatting of CRUD tool responses (`di_crud_rendering.py`).
//!
//! The renderers take the API response as JSON. Timestamps arrive as ISO
//! strings (see [`super::timefmt::format_timestamp`]); the create renderer
//! takes the current time as an argument instead of reading the clock, so
//! its output is deterministic.

use toolkit_core::Json;

use super::capture::{capture_from_response, Capture, CaptureLimits, CodeCapture};
use super::consts::SNAPSHOT_SIGNAL_TYPE;
use super::location::{location_from_response, render_location_block, Location};
use super::pytext::{get_array, get_text, get_truthy_text};
use super::timefmt::{format_timestamp, UtcTime};
use crate::dynamodb::pyfmt;

/// The label of each capture limit, in `CaptureLimits::entries` order.
const LIMIT_LABELS: [&str; 8] = [
    "Max Hits",
    "Max String Length",
    "Max Collection Width",
    "Max Collection Depth",
    "Max Stack Frames",
    "Max Stack Trace Size",
    "Max Object Depth",
    "Max Fields Per Object",
];

fn push_limit_lines(output: &mut String, limits: &CaptureLimits) {
    for (label, (_, value)) in LIMIT_LABELS.iter().zip(limits.entries()) {
        if *value != Json::Null {
            output.push_str(&format!("- {label}: {}\n", pyfmt::text(value)));
        }
    }
}

fn render_create_capture_limits(limits: &CaptureLimits) -> String {
    if limits.is_empty() {
        return String::new();
    }
    let mut output = String::from("\nCAPTURE LIMITS:\n");
    push_limit_lines(&mut output, limits);
    output
}

fn enabled(flag: bool) -> &'static str {
    if flag {
        "Enabled"
    } else {
        "Disabled"
    }
}

/// What the create renderer needs besides the API response.
#[derive(Debug, Clone, Copy)]
pub struct CreateSummary<'a> {
    /// BREAKPOINT or PROBE.
    pub normalized_type: &'a str,
    /// `service`.
    pub service: &'a str,
    /// `environment`.
    pub environment: &'a str,
    /// The resolved location.
    pub location: &'a Location,
    /// `ttl_hours`, when the caller asked for an expiry.
    pub ttl_hours: Option<i64>,
    /// `capture_arguments` as sent.
    pub capture_arguments: Option<&'a [String]>,
    /// The `capture_locals` actually used.
    pub capture_locals: Option<&'a [String]>,
    /// True when `line_number` was set.
    pub is_line_level: bool,
    /// The resolved `CaptureReturn`.
    pub capture_return: Option<bool>,
    /// The resolved `CaptureStackTrace`.
    pub capture_stack_trace: Option<bool>,
    /// The capture limits as sent.
    pub limits: &'a CaptureLimits,
    /// The `attribute_filters` as sent (empty when none).
    pub attribute_filters: &'a [Json],
    /// The current time, used as `CreatedAt` when the response has none.
    pub now: UtcTime,
}

fn hash_for_block(response: &Json) -> Option<String> {
    match response.get("LocationHash") {
        None => Some("N/A".to_string()),
        Some(value) if pyfmt::truthy(value) => Some(pyfmt::text(value)),
        Some(_) => None,
    }
}

/// Renders the success message for a created instrumentation configuration.
#[must_use]
pub fn render_create_success_message(response: &Json, summary: &CreateSummary<'_>) -> String {
    let location_hash_text = get_text(response, "LocationHash", "N/A");
    let arn = get_text(response, "ARN", "N/A");
    let created_at = format_timestamp(response.get("CreatedAt"), &summary.now.format_seconds_z());
    let actual_expires_at = format_timestamp(response.get("ExpiresAt"), "");

    let mut message = format!(
        "Successfully created {t} instrumentation\n\
         \n\
         INSTRUMENTATION CREATED:\n\
         - Type: {t}\n\
         - Service: {}\n\
         - Environment: {}\n\
         - SignalType: {SNAPSHOT_SIGNAL_TYPE}\n\
         - ARN: {arn}\n\
         - CreatedAt: {created_at}\n",
        summary.service,
        summary.environment,
        t = summary.normalized_type,
    );

    if actual_expires_at.is_empty() {
        message.push_str("- Expires: Never (unless deleted)\n");
    } else {
        let suffix = match summary.ttl_hours {
            Some(hours) => format!(
                " (requested {hours} hour{})",
                if hours == 1 { "" } else { "s" }
            ),
            None => String::new(),
        };
        message.push_str(&format!("- Expires: {actual_expires_at}{suffix}\n"));
    }

    message.push_str("\nLOCATION:\n");
    message.push_str(&render_location_block(
        summary.location,
        hash_for_block(response).as_deref(),
    ));
    message.push_str("\nCAPTURE CONFIGURATION:\n");

    if !summary.is_line_level {
        match summary.capture_arguments.filter(|names| !names.is_empty()) {
            Some(names) => message.push_str(&format!("- Arguments: {}\n", names.join(", "))),
            None => message.push_str("- Arguments: (none)\n"),
        }
    }

    if let Some(names) = summary.capture_locals.filter(|names| !names.is_empty()) {
        message.push_str(&format!("- Local Variables: {}\n", names.join(", ")));
    }

    if !summary.is_line_level {
        message.push_str(&format!(
            "- Return Values: {}\n",
            enabled(summary.capture_return.unwrap_or(false))
        ));
    }
    message.push_str(&format!(
        "- Stack Traces: {}\n",
        enabled(summary.capture_stack_trace.unwrap_or(false))
    ));
    message.push_str(&render_create_capture_limits(summary.limits));

    if !summary.attribute_filters.is_empty() {
        message.push_str(&format!(
            "\nATTRIBUTE FILTERS: {} filter group(s) applied\n",
            summary.attribute_filters.len()
        ));
    }

    let expected_ready = if summary.normalized_type == "PROBE" {
        "~10-12 min"
    } else {
        "~1-2 min"
    };
    message.push_str(&format!(
        "\nNOTE: Allow {expected_ready} before this configuration reports READY. \
         Status checks immediately after creation may return no events yet \u{2014} \
         wait and re-check rather than recreating.\n"
    ));

    message.push_str(&format!(
        "\nTIP: Use this LocationHash to delete: \
         delete_instrumentation(location_hash=\"{location_hash_text}\")"
    ));
    message
}

fn push_names(output: &mut String, label: &str, names: Option<&[String]>, empty_label: &str) {
    match names {
        None => output.push_str(&format!("- {label}: (not set)\n")),
        Some([]) => output.push_str(&format!("- {label}: ({empty_label})\n")),
        Some(items) => output.push_str(&format!("- {label}: {}\n", items.join(", "))),
    }
}

fn push_list_capture(output: &mut String, capture: &CodeCapture) {
    output.push_str(&format!("- Return: {}\n", enabled(capture.capture_return)));
    output.push_str(&format!(
        "- Stack Traces: {}\n",
        enabled(capture.capture_stack_trace)
    ));
    push_names(
        output,
        "Arguments",
        capture.capture_arguments.as_deref(),
        "empty list",
    );
    push_names(
        output,
        "Locals",
        capture.capture_locals.as_deref(),
        "empty list",
    );

    let limits = &capture.limits;
    if !limits.is_empty() {
        let mut parts: Vec<String> = Vec::new();
        if limits.max_hits != Json::Null {
            parts.push(format!("MaxHits={}", pyfmt::text(&limits.max_hits)));
        }
        if limits.max_string_length != Json::Null {
            parts.push(format!(
                "MaxStringLen={}",
                pyfmt::text(&limits.max_string_length)
            ));
        }
        if limits.max_collection_width != Json::Null {
            parts.push(format!(
                "MaxCollWidth={}",
                pyfmt::text(&limits.max_collection_width)
            ));
        }
        if !parts.is_empty() {
            output.push_str(&format!("- Limits: {}\n", parts.join(", ")));
        }
    }
}

/// Renders the output for a list-instrumentations result.
#[must_use]
pub fn render_list_instrumentations_output(
    data: &Json,
    normalized_type: &str,
    service: &str,
    environment: &str,
) -> String {
    let configs = get_array(data, "LatestConfigurations");

    if configs.is_empty() {
        return format!(
            "No active {normalized_type} instrumentations found\n\
             \n\
             Service: {service}\n\
             Environment: {environment}\n\
             \n\
             TIP: Use create_instrumentation to add instrumentations."
        );
    }

    let mut output = format!(
        "Active {normalized_type} Instrumentations ({} found)\n\
         \n\
         Service: {service}\n\
         Environment: {environment}\n\
         Synced At: {}\n\
         \n",
        configs.len(),
        format_timestamp(data.get("SyncedAt"), "N/A"),
    );

    let rule = "=".repeat(60);
    for (index, config) in configs.iter().enumerate() {
        output.push_str(&format!(
            "{rule}\nINSTRUMENTATION #{}\n{rule}\nLOCATION:\n",
            index + 1
        ));
        let location = location_from_response(config.get("Location").unwrap_or(&Json::Null));
        let hash = get_truthy_text(config, "LocationHash");
        output.push_str(&render_location_block(&location, hash.as_deref()));

        output.push_str("\nCAPTURE SETTINGS:\n");
        match capture_from_response(config.get("CaptureConfiguration").unwrap_or(&Json::Null)) {
            Capture::Code(capture) => push_list_capture(&mut output, &capture),
            Capture::Unknown(_) => output.push_str("- Capture payload could not be parsed.\n"),
        }

        output.push_str(&format!(
            "\nTIMING:\n\
             - Created: {}\n\
             - Expires: {}\n\
             \n\
             Description: {}\n\
             ARN: {}\n\
             \n",
            format_timestamp(config.get("CreatedAt"), "N/A"),
            format_timestamp(config.get("ExpiresAt"), "Never"),
            get_text(config, "Description", "N/A"),
            get_text(config, "ARN", "N/A"),
        ));
    }

    if let Some(token) = get_truthy_text(data, "NextToken") {
        output.push_str(&format!(
            "\nPAGINATION: More results available. Use next_token=\"{token}\" \
             to retrieve next page."
        ));
    }

    output
}

/// Renders the output for a single get-instrumentation result.
#[must_use]
pub fn render_get_instrumentation_output(
    config: &Json,
    service: &str,
    environment: &str,
) -> String {
    let mut output = format!(
        "INSTRUMENTATION CONFIGURATION\n\
         \n\
         TYPE: {}\n\
         SERVICE: {service}\n\
         ENVIRONMENT: {environment}\n\
         SIGNAL TYPE: {}\n\
         \n\
         LOCATION:\n",
        get_text(config, "InstrumentationType", "N/A"),
        get_text(config, "SignalType", SNAPSHOT_SIGNAL_TYPE),
    );
    let location = location_from_response(config.get("Location").unwrap_or(&Json::Null));
    let hash = get_truthy_text(config, "LocationHash");
    output.push_str(&render_location_block(&location, hash.as_deref()));

    output.push_str("\nCAPTURE CONFIGURATION:\n");
    match capture_from_response(config.get("CaptureConfiguration").unwrap_or(&Json::Null)) {
        Capture::Code(capture) => {
            output.push_str(&format!(
                "- Return Values: {}\n",
                enabled(capture.capture_return)
            ));
            output.push_str(&format!(
                "- Stack Traces: {}\n",
                enabled(capture.capture_stack_trace)
            ));
            push_names(
                &mut output,
                "Arguments",
                capture.capture_arguments.as_deref(),
                "empty list",
            );
            push_names(
                &mut output,
                "Local Variables",
                capture.capture_locals.as_deref(),
                "empty list",
            );
            if !capture.limits.is_empty() {
                output.push_str("\nCAPTURE LIMITS:\n");
                push_limit_lines(&mut output, &capture.limits);
            }
        }
        Capture::Unknown(_) => output.push_str("- Capture payload could not be parsed.\n"),
    }

    let filters = get_array(config, "AttributeFilters");
    if !filters.is_empty() {
        output.push_str(&format!(
            "\nATTRIBUTE FILTERS: {} filter group(s)\n",
            filters.len()
        ));
        for (index, group) in filters.iter().enumerate() {
            output.push_str(&format!("  Group {}: {}\n", index + 1, pyfmt::text(group)));
        }
    }

    output.push_str(&format!(
        "\nMETADATA:\n\
         - Description: {}\n\
         - Created: {}\n\
         - Expires: {}\n\
         - ARN: {}\n",
        get_text(config, "Description", "N/A"),
        format_timestamp(config.get("CreatedAt"), "N/A"),
        format_timestamp(config.get("ExpiresAt"), "Never"),
        get_text(config, "ARN", "N/A"),
    ));
    output
}

/// Renders a batch-delete response (scope or ARN mode). `service` and
/// `environment` are only printed when non-empty.
#[must_use]
pub fn format_batch_delete_response(
    mode: &str,
    data: &Json,
    instrumentation_type: &str,
    service: Option<&str>,
    environment: Option<&str>,
) -> String {
    let successful = get_array(data, "SuccessfulDeletions");
    let errors = get_array(data, "Errors");

    let mut output = format!(
        "BATCH DELETE COMPLETED\n\
         \n\
         Mode: {mode}\n\
         InstrumentationType: {instrumentation_type}\n\
         DeletedCount: {}\n\
         SuccessfulDeletions: {}\n\
         Errors: {}\n",
        get_text(data, "DeletedCount", "0"),
        successful.len(),
        errors.len(),
    );
    if let Some(service) = service.filter(|text| !text.is_empty()) {
        output.push_str(&format!("Service: {service}\n"));
    }
    if let Some(environment) = environment.filter(|text| !text.is_empty()) {
        output.push_str(&format!("Environment: {environment}\n"));
    }

    if !successful.is_empty() {
        output.push_str("\nSUCCESSFUL DELETIONS:\n");
        for (index, item) in successful.iter().enumerate() {
            let number = index + 1;
            if let Some(arn) = get_truthy_text(item, "ResourceArn") {
                output.push_str(&format!("- Item {number}: ResourceArn={arn}\n"));
            } else {
                let signal_type =
                    get_truthy_text(item, "SignalType").unwrap_or_else(|| "N/A".to_string());
                let location_hash =
                    get_truthy_text(item, "LocationHash").unwrap_or_else(|| "N/A".to_string());
                output.push_str(&format!(
                    "- Item {number}: SignalType={signal_type} | LocationHash={location_hash}\n"
                ));
            }
        }
    }

    if !errors.is_empty() {
        output.push_str("\nDELETE ERRORS:\n");
        for (index, item) in errors.iter().enumerate() {
            output.push_str(&format!(
                "- Item {}: ResourceArn={} | Code={} | Message={}\n",
                index + 1,
                get_text(item, "ResourceArn", "N/A"),
                get_text(item, "Code", "N/A"),
                get_text(item, "Message", "N/A"),
            ));
        }
    }

    output
}
