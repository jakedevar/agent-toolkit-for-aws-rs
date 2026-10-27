//! Region resolution for the dynamic-instrumentation commands
//! (`di_region.py`).
//!
//! Precedence: explicit `--region` > `AWS_REGION` > `AWS_DEFAULT_REGION` >
//! `us-east-1`. `AWS_PROFILE` is for credentials only; the profile's own
//! region is deliberately ignored. An empty value counts as unset at every
//! level, like Python's `or` chain.

/// The fallback region.
pub const DEFAULT_REGION: &str = "us-east-1";

/// Resolves the region with an injected environment lookup (deterministic
/// in tests).
#[must_use]
pub fn resolve_region(explicit: Option<&str>, env: &dyn Fn(&str) -> Option<String>) -> String {
    if let Some(region) = explicit.filter(|region| !region.is_empty()) {
        return region.to_string();
    }
    for name in ["AWS_REGION", "AWS_DEFAULT_REGION"] {
        if let Some(region) = env(name).filter(|region| !region.is_empty()) {
            return region;
        }
    }
    DEFAULT_REGION.to_string()
}

/// [`resolve_region`] against the process environment.
#[must_use]
pub fn resolve_region_from_env(explicit: Option<&str>) -> String {
    resolve_region(explicit, &|name| std::env::var(name).ok())
}
