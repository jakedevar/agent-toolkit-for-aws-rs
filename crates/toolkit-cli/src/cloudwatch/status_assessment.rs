//! Consolidated status assessment for dynamic instrumentation
//! (`di_status_assessment.py`).
//!
//! The consolidated check answers one question - what is the high-level
//! state of this instrumentation right now? - by querying three status
//! signals (ACTIVE, READY, ERROR) in priority order over a time window. This
//! module owns the time-window policy (ACTIVE is only meaningful after the
//! instrumentation was created, so its window is clamped to
//! `max(created_at, requested_start)`), the check ordering (ACTIVE wins,
//! then READY, then ERROR decides between ERROR and PENDING) and the verdict
//! shape. I/O lives in the caller: [`assess`] takes a `check_status`
//! callback so the policy is testable without AWS.

use toolkit_core::Json;

use super::timefmt::UtcTime;

/// The four ISO-formatted time strings every consolidated renderer needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TimeWindow {
    /// When the instrumentation was created.
    pub created_at: String,
    /// The start of the requested window.
    pub requested_start: String,
    /// `max(created_at, requested_start)`.
    pub active_query_start: String,
    /// The end of the window.
    pub query_end: String,
}

/// What one status query returned: `(has_events, events, error)`.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct StatusCheckResult {
    /// True when at least one event came back.
    pub has_events: bool,
    /// The status events (objects with `Time`, `ErrorCause`, ...).
    pub events: Vec<Json>,
    /// An error (or `Skipped: ...` note) instead of a result.
    pub error: Option<String>,
}

/// The outcome of the consolidated check.
#[derive(Debug, Clone, PartialEq)]
pub enum Verdict {
    /// ACTIVE events were found in the (clamped) ACTIVE window.
    Active {
        /// The ACTIVE check.
        active: StatusCheckResult,
    },
    /// ACTIVE not confirmed, but READY events were found.
    Ready {
        /// The ACTIVE check.
        active: StatusCheckResult,
        /// The READY check.
        ready: StatusCheckResult,
    },
    /// Neither ACTIVE nor READY confirmed; the ERROR check decides between
    /// ERROR and PENDING by whether it has events.
    ErrorOrPending {
        /// The ACTIVE check.
        active: StatusCheckResult,
        /// The READY check.
        ready: StatusCheckResult,
        /// The ERROR check.
        error: StatusCheckResult,
    },
}

/// Runs the consolidated status assessment.
///
/// The caller parses ISO inputs, verifies `query_end > requested_start`
/// (that error message belongs to the tool layer) and provides the
/// `check_status(status, start, end)` callback that issues the AWS query.
/// Returns the verdict and the time window; the renderer takes both.
pub fn assess(
    created_at: UtcTime,
    requested_start: UtcTime,
    query_end: UtcTime,
    check_status: &mut dyn FnMut(&str, UtcTime, UtcTime) -> StatusCheckResult,
) -> (Verdict, TimeWindow) {
    let active_query_start = created_at.max(requested_start);

    let time_window = TimeWindow {
        created_at: created_at.format_seconds_z(),
        requested_start: requested_start.format_seconds_z(),
        active_query_start: active_query_start.format_seconds_z(),
        query_end: query_end.format_seconds_z(),
    };

    let active = if query_end > active_query_start {
        check_status("ACTIVE", active_query_start, query_end)
    } else {
        StatusCheckResult {
            has_events: false,
            events: Vec::new(),
            error: Some(format!(
                "Skipped: ACTIVE query window is empty after applying created_at clamp \
                 (start={}, end={})",
                time_window.active_query_start, time_window.query_end
            )),
        }
    };

    if active.has_events {
        return (Verdict::Active { active }, time_window);
    }

    let ready = check_status("READY", requested_start, query_end);
    if ready.has_events {
        return (Verdict::Ready { active, ready }, time_window);
    }

    let error = check_status("ERROR", requested_start, query_end);
    (
        Verdict::ErrorOrPending {
            active,
            ready,
            error,
        },
        time_window,
    )
}
