//! Formatting of status tool responses (`di_status_rendering.py`).
//!
//! Every prose line is the legacy text, including the quirks the tests pin:
//! the READY and ERROR/PENDING renderers build on the ACTIVE renderer and
//! cut its trailing `OVERALL STATUS: ACTIVE not confirmed yet` line.

use toolkit_core::Json;

use super::consts::{resolve_snapshot_log_group, SNAPSHOT_SIGNAL_TYPE};
use super::location::{location_from_response, render_location_block};
use super::pytext::{get_array, get_text, get_truthy_text, safe_text};
use super::status_assessment::{StatusCheckResult, TimeWindow, Verdict};
use super::timefmt::format_timestamp;

const ACTIVE_NOT_CONFIRMED: &str = "OVERALL STATUS: ACTIVE not confirmed yet\n";

/// Renders the explicit status-history response.
#[must_use]
pub fn render_get_instrumentation_configuration_status_output(
    data: &Json,
    normalized_type: &str,
    service: &str,
    environment: &str,
    requested_status: &str,
) -> String {
    let events = get_array(data, "Events");

    let mut output = format!(
        "INSTRUMENTATION STATUS\n\
         \n\
         TYPE: {normalized_type}\n\
         SERVICE: {}\n\
         ENVIRONMENT: {}\n\
         SIGNAL TYPE: {}\n\
         REQUESTED STATUS FILTER: {requested_status}\n\
         CURRENT STATUS: {}\n\
         \n\
         LOCATION:\n",
        get_text(data, "Service", service),
        get_text(data, "Environment", environment),
        get_text(data, "SignalType", SNAPSHOT_SIGNAL_TYPE),
        get_text(data, "Status", "N/A"),
    );
    let location = location_from_response(data.get("Location").unwrap_or(&Json::Null));
    let hash = get_truthy_text(data, "LocationHash");
    output.push_str(&render_location_block(&location, hash.as_deref()));
    output.push_str(&format!("- Events Returned: {}\n", events.len()));

    if events.is_empty() {
        output.push_str(&format!(
            "- Status Confirmation: NOT CONFIRMED (no {requested_status} events)\n"
        ));
    } else {
        output.push_str(&format!(
            "- Status Confirmation: CONFIRMED ({requested_status} events present)\n"
        ));
    }

    output.push_str(
        "- Interpretation Rule: Do not treat CURRENT STATUS as confirmed unless \
         STATUS EVENTS contain entries.\n",
    );

    if requested_status == "ACTIVE" && events.is_empty() {
        output.push_str(
            "- ACTIVE Clarification: Breakpoint is not confirmed as hit yet. \
             If READY is not yet confirmed, check READY first. \
             Otherwise wait for traffic and poll ACTIVE again.\n",
        );
    }

    output.push_str("\nSTATUS EVENTS:\n");
    if events.is_empty() {
        output.push_str(&format!("- No {requested_status} status events found\n"));
    } else {
        for (index, event) in events.iter().enumerate() {
            let event_time = format_timestamp(event.get("Time"), "N/A");
            output.push_str(&format!("- Event {}: {event_time}", index + 1));
            if let Some(cause) = get_truthy_text(event, "ErrorCause")
                .as_deref()
                .map(safe_text)
            {
                output.push_str(&format!(" | ErrorCause: {cause}"));
            }
            output.push('\n');
        }
    }

    if let Some(token) = get_truthy_text(data, "NextToken") {
        output.push_str(&format!(
            "\nPAGINATION: More results available. Use next_token=\"{token}\" \
             to retrieve next page."
        ));
    }

    output
}

fn render_status_section(
    title: &str,
    start_time: &str,
    end_time: &str,
    check: &StatusCheckResult,
    include_error_cause: bool,
) -> String {
    let mut output = format!("{title} STATUS:\n");
    output.push_str(&format!("- Time Window: {start_time} to {end_time}\n"));
    if let Some(error) = check.error.as_deref().filter(|error| !error.is_empty()) {
        if error.starts_with("Skipped:") {
            output.push_str(&format!("- Check Skipped: {error}\n"));
        } else {
            output.push_str(&format!("- Check Failed: {error}\n"));
        }
        return output;
    }

    if check.has_events {
        output.push_str(&format!(
            "- Confirmed: YES ({} event(s))\n",
            check.events.len()
        ));
        for (index, event) in check.events.iter().take(3).enumerate() {
            output.push_str(&format!(
                "  - Event {}: {}",
                index + 1,
                format_timestamp(event.get("Time"), "N/A")
            ));
            if include_error_cause {
                output.push_str(&format!(
                    " | ErrorCause: {}",
                    safe_text(&get_text(event, "ErrorCause", "Unknown"))
                ));
            }
            output.push('\n');
        }
        if check.events.len() > 3 {
            output.push_str(&format!("  - ... and {} more\n", check.events.len() - 3));
        }
    } else {
        output.push_str(&format!("- Confirmed: NO (no {title} events found)\n"));
    }
    output
}

/// What identifies the instrumentation in a consolidated status response.
#[derive(Debug, Clone, Copy)]
pub struct StatusSubject<'a> {
    /// `location_hash`.
    pub location_hash: &'a str,
    /// `service`.
    pub service: &'a str,
    /// `environment`.
    pub environment: &'a str,
    /// `normalized_type` (BREAKPOINT or PROBE).
    pub normalized_type: &'a str,
}

/// Renders a consolidated status response when ACTIVE is confirmed or
/// checked first.
#[must_use]
pub fn render_consolidated_active_status_output(
    subject: &StatusSubject<'_>,
    window: &TimeWindow,
    active: &StatusCheckResult,
) -> String {
    let mut output = format!(
        "CONSOLIDATED STATUS CHECK\n\
         \n\
         INSTRUMENTATION INFO:\n\
         - LocationHash: {}\n\
         - Service: {}\n\
         - Environment: {}\n\
         - Type: {}\n\
         \n\
         TIME RANGE:\n\
         - Created At: {}\n\
         - Requested Start: {}\n\
         - ACTIVE Query Start: {}\n\
         - Query End: {}\n\
         \n",
        subject.location_hash,
        subject.service,
        subject.environment,
        subject.normalized_type,
        window.created_at,
        window.requested_start,
        window.active_query_start,
        window.query_end,
    );
    output.push_str(&render_status_section(
        "ACTIVE",
        &window.active_query_start,
        &window.query_end,
        active,
        false,
    ));
    output.push('\n');

    if active.has_events {
        output.push_str(&format!(
            "SNAPSHOT QUERY TIP: Try these timestamps with search_snapshots_for_status_event\n  \
             (log group: \"{}\")\n  \
             Oldest first \u{2014} older events are more likely to have snapshots ingested:\n",
            resolve_snapshot_log_group(subject.service)
        ));
        let newest: Vec<&Json> = active.events.iter().take(5).collect();
        for (index, event) in newest.iter().rev().enumerate() {
            let mut label = if index == 0 {
                " (oldest, try first)"
            } else {
                ""
            };
            if index + 1 == newest.len() && index > 0 {
                label = " (most recent)";
            }
            output.push_str(&format!(
                "  - status_timestamp=\"{}\"{label}\n",
                format_timestamp(event.get("Time"), "")
            ));
        }
        output.push('\n');
        output.push_str("OVERALL STATUS: ACTIVE \u{2713} (breakpoint is being hit)\n");
        return output;
    }

    output.push_str(ACTIVE_NOT_CONFIRMED);
    output
}

fn without_active_not_confirmed(mut output: String) -> String {
    if output.ends_with(ACTIVE_NOT_CONFIRMED) {
        output.truncate(output.len() - ACTIVE_NOT_CONFIRMED.len());
    }
    output
}

/// Renders a consolidated status response when READY is the best confirmed
/// state.
#[must_use]
pub fn render_consolidated_ready_status_output(
    subject: &StatusSubject<'_>,
    window: &TimeWindow,
    active: &StatusCheckResult,
    ready: &StatusCheckResult,
) -> String {
    let mut output = without_active_not_confirmed(render_consolidated_active_status_output(
        subject, window, active,
    ));
    output.push_str(&render_status_section(
        "READY",
        &window.requested_start,
        &window.query_end,
        ready,
        false,
    ));
    output.push_str("\nOVERALL STATUS: READY (waiting for traffic)\n");
    output
}

/// Renders a consolidated status response for ERROR or PENDING outcomes.
#[must_use]
pub fn render_consolidated_error_or_pending_status_output(
    subject: &StatusSubject<'_>,
    window: &TimeWindow,
    active: &StatusCheckResult,
    ready: &StatusCheckResult,
    error: &StatusCheckResult,
) -> String {
    let mut output = without_active_not_confirmed(render_consolidated_active_status_output(
        subject, window, active,
    ));
    output.push('\n');
    output.push_str(&render_status_section(
        "READY",
        &window.requested_start,
        &window.query_end,
        ready,
        false,
    ));
    output.push('\n');
    output.push_str(&render_status_section(
        "ERROR",
        &window.requested_start,
        &window.query_end,
        error,
        true,
    ));

    output.push_str("\nOVERALL STATUS: ");
    if error.has_events {
        let error_cause = error.events.first().map_or_else(
            || "Unknown".to_string(),
            |event| safe_text(&get_text(event, "ErrorCause", "Unknown")),
        );
        output.push_str(&format!("ERROR ({error_cause})\n"));
        output.push_str("\nTROUBLESHOOTING:\n");
        match error_cause.as_str() {
            "FILE_NOT_FOUND" => output.push_str("- Verify file_path is correct\n"),
            "METHOD_NOT_FOUND" => {
                output.push_str("- Verify method_name and code_unit are correct\n");
                output.push_str("- Check if the function is loaded at runtime\n");
            }
            "LINE_NOT_EXECUTABLE" => output.push_str(
                "- Verify line_number points to executable code (not comment/blank/declaration)\n",
            ),
            other => output.push_str(&format!(
                "- Check instrumentation configuration for {other}\n"
            )),
        }
    } else {
        output.push_str(
            "PENDING (no ACTIVE, READY, or ERROR events yet - wait longer or check configuration)\n",
        );
        output.push_str("\nNOTE: Status events can take 1-2 minutes to appear after creation.\n");
    }

    output
}

/// Dispatches a [`Verdict`] to the matching consolidated-status renderer.
#[must_use]
pub fn render_status_assessment(
    verdict: &Verdict,
    subject: &StatusSubject<'_>,
    window: &TimeWindow,
) -> String {
    match verdict {
        Verdict::Active { active } => {
            render_consolidated_active_status_output(subject, window, active)
        }
        Verdict::Ready { active, ready } => {
            render_consolidated_ready_status_output(subject, window, active, ready)
        }
        Verdict::ErrorOrPending {
            active,
            ready,
            error,
        } => render_consolidated_error_or_pending_status_output(
            subject, window, active, ready, error,
        ),
    }
}
