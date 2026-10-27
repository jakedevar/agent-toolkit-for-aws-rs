//! Shared constants for dynamic instrumentation support (`di_constants.py`).

/// The only signal type the instrumentation API accepts.
pub const SNAPSHOT_SIGNAL_TYPE: &str = "SNAPSHOT";

/// Snapshots are written to a per-service CloudWatch Logs group;
/// `{service_name}` is substituted at query time.
pub const SNAPSHOT_LOG_GROUP_TEMPLATE: &str = "/aws/service-events/{service_name}";

/// Resolves the per-service snapshot log group name
/// (`resolve_snapshot_log_group`). The substitution is a single
/// replacement, so braces inside `service_name` are kept as they are.
#[must_use]
pub fn resolve_snapshot_log_group(service_name: &str) -> String {
    SNAPSHOT_LOG_GROUP_TEMPLATE.replace("{service_name}", service_name)
}
